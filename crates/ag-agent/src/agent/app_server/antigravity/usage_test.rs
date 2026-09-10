use crate::agent::app_server::antigravity::usage::{TokenUsage, TurnUsageTracker};

#[test]
fn completed_step_usage_wins_over_cumulative_session_usage() {
    // Arrange
    let mut tracker = TurnUsageTracker::default();
    tracker.record_step(&serde_json::json!({
        "step_index": 4,
        "state": "DONE",
        "usage": {"input_tokens": 278, "output_tokens": 4},
    }));
    let result = serde_json::json!({
        "usage": {"input_tokens": 30_662, "output_tokens": 8},
    });
    let mut previous = Some(TokenUsage {
        input_tokens: 30_384,
        output_tokens: 4,
    });

    // Act
    let usage = tracker.finish(&result, &mut previous);

    // Assert
    assert_eq!(
        usage,
        TokenUsage {
            input_tokens: 278,
            output_tokens: 4,
        }
    );
    assert_eq!(
        previous,
        Some(TokenUsage {
            input_tokens: 30_662,
            output_tokens: 8,
        })
    );
}

#[test]
fn cumulative_usage_delta_is_fallback_when_steps_omit_usage() {
    // Arrange
    let tracker = TurnUsageTracker::default();
    let result = serde_json::json!({
        "usage": {"input_tokens": 150, "output_tokens": 27},
    });
    let mut previous = Some(TokenUsage {
        input_tokens: 100,
        output_tokens: 20,
    });

    // Act
    let usage = tracker.finish(&result, &mut previous);

    // Assert
    assert_eq!(
        usage,
        TokenUsage {
            input_tokens: 50,
            output_tokens: 7,
        }
    );
}

#[test]
fn active_and_usage_free_steps_do_not_change_turn_total() {
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
    assert_eq!(usage, TokenUsage::default());
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
        TokenUsage {
            input_tokens: 12,
            output_tokens: 3,
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
    assert_eq!(
        usage,
        TokenUsage {
            input_tokens: 15,
            output_tokens: 4,
        }
    );
    assert_eq!(previous, Some(usage));
}

#[test]
fn cumulative_usage_delta_saturates_after_provider_counter_reset() {
    // Arrange
    let tracker = TurnUsageTracker::default();
    let result = serde_json::json!({
        "usage": {"input_tokens": 2, "output_tokens": 1},
    });
    let mut previous = Some(TokenUsage {
        input_tokens: 100,
        output_tokens: 20,
    });

    // Act
    let usage = tracker.finish(&result, &mut previous);

    // Assert
    assert_eq!(usage, TokenUsage::default());
    assert_eq!(
        previous,
        Some(TokenUsage {
            input_tokens: 2,
            output_tokens: 1,
        })
    );
}
