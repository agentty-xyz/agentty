//! Antigravity per-turn usage accounting.

use std::collections::HashMap;

use serde_json::Value;

/// Input/output token counters from one Antigravity usage object.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct TokenUsage {
    pub(super) input_tokens: u64,
    pub(super) output_tokens: u64,
}

impl TokenUsage {
    fn saturating_sub(self, previous: Self) -> Self {
        Self {
            input_tokens: self.input_tokens.saturating_sub(previous.input_tokens),
            output_tokens: self.output_tokens.saturating_sub(previous.output_tokens),
        }
    }
}

/// Collects model usage from completed steps within the active turn.
#[derive(Default)]
pub(super) struct TurnUsageTracker {
    completed_steps: HashMap<u64, TokenUsage>,
}

impl TurnUsageTracker {
    /// Records the latest usage for one completed step event.
    pub(super) fn record_step(&mut self, step_update: &Value) {
        if !step_update
            .get("state")
            .and_then(Value::as_str)
            .is_some_and(|state| state.eq_ignore_ascii_case("done"))
        {
            return;
        }
        let Some(step_index) = step_update.get("step_index").and_then(Value::as_u64) else {
            return;
        };
        let Some(step_usage) = step_update.get("usage").and_then(parse_usage) else {
            return;
        };

        self.completed_steps.insert(step_index, step_usage);
    }

    /// Selects per-turn counters and advances the cumulative result baseline.
    pub(super) fn finish(
        self,
        result: &Value,
        previous_cumulative: &mut Option<TokenUsage>,
    ) -> TokenUsage {
        let cumulative = result.get("usage").and_then(parse_usage);
        let step_total =
            self.completed_steps
                .values()
                .copied()
                .fold(TokenUsage::default(), |total, usage| TokenUsage {
                    input_tokens: total.input_tokens.saturating_add(usage.input_tokens),
                    output_tokens: total.output_tokens.saturating_add(usage.output_tokens),
                });
        let has_step_usage = !self.completed_steps.is_empty();
        let result_usage = if has_step_usage {
            step_total
        } else if let (Some(cumulative), Some(previous)) = (cumulative, *previous_cumulative) {
            cumulative.saturating_sub(previous)
        } else {
            cumulative.unwrap_or_default()
        };
        if cumulative.is_some() {
            *previous_cumulative = cumulative;
        }

        result_usage
    }
}

/// Parses one provider usage object.
fn parse_usage(usage: &Value) -> Option<TokenUsage> {
    Some(TokenUsage {
        input_tokens: usage.get("input_tokens")?.as_u64()?,
        output_tokens: usage.get("output_tokens")?.as_u64()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
