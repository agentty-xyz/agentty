//! Codex app-server usage extraction helpers.

use serde_json::Value;

use crate::{CodexRateLimits, RateLimitWindow};

/// Extracts the latest account rate-limit snapshot from a Codex response.
pub(super) fn extract_rate_limits(response_value: &Value) -> Option<CodexRateLimits> {
    let method = response_value.get("method").and_then(Value::as_str);
    let rate_limits = if method == Some("account/rateLimits/updated") {
        response_value.get("params")?.get("rateLimits")?
    } else {
        response_value.get("result")?.get("rateLimits")?
    };

    let snapshot = CodexRateLimits {
        primary: rate_limits.get("primary").and_then(parse_rate_limit_window),
        secondary: rate_limits
            .get("secondary")
            .and_then(parse_rate_limit_window),
    };
    if snapshot == CodexRateLimits::default() {
        return None;
    }

    Some(snapshot)
}

/// Parses one Codex quota-window object while accepting integer or fractional
/// usage percentages from different app-server versions.
fn parse_rate_limit_window(value: &Value) -> Option<RateLimitWindow> {
    if value.is_null() {
        return None;
    }

    let used_percent = value
        .get("usedPercent")
        .or_else(|| value.get("used_percent"))
        .and_then(Value::as_f64)
        .and_then(|percent| percent.round().clamp(0.0, 100.0).to_string().parse().ok())?;
    let window_duration_mins = value
        .get("windowDurationMins")
        .or_else(|| value.get("window_duration_mins"))
        .and_then(Value::as_u64);
    let resets_at = value
        .get("resetsAt")
        .or_else(|| value.get("resets_at"))
        .and_then(Value::as_i64);

    Some(RateLimitWindow {
        resets_at,
        used_percent,
        window_duration_mins,
    })
}

/// Resolves final turn usage by preferring `turn/completed` payload usage and
/// falling back to the last seen usage update when completion omits it.
pub(super) fn resolve_turn_usage(
    completed_turn_usage: Option<(u64, u64)>,
    latest_stream_usage: Option<(u64, u64)>,
) -> (u64, u64) {
    completed_turn_usage
        .or(latest_stream_usage)
        .unwrap_or((0, 0))
}

/// Updates usage trackers for one app-server response line.
///
/// Usage is read from modern `thread/tokenUsage/updated` notifications first,
/// then from legacy `turn.usage` payloads for backwards compatibility.
pub(super) fn update_turn_usage_from_response(
    response_value: &Value,
    expected_turn_id: Option<&str>,
    completed_turn_usage: &mut Option<(u64, u64)>,
    latest_stream_usage: &mut Option<(u64, u64)>,
) {
    if let Some(turn_usage) = extract_thread_token_usage_for_turn(response_value, expected_turn_id)
    {
        *latest_stream_usage = Some(turn_usage);

        return;
    }

    if let Some(turn_usage) = extract_turn_usage_for_turn(response_value, expected_turn_id) {
        if response_value.get("method").and_then(Value::as_str) == Some("turn/completed") {
            *completed_turn_usage = Some(turn_usage);
        } else {
            *latest_stream_usage = Some(turn_usage);
        }
    }
}

/// Extracts input/output token usage from `turn.usage` payloads.
pub(super) fn extract_turn_usage(response_value: &Value) -> Option<(u64, u64)> {
    let turn = response_value
        .get("params")
        .and_then(|params| params.get("turn"))?;

    let usage = turn.get("usage")?;

    let input_tokens = usage
        .get("inputTokens")
        .and_then(Value::as_u64)
        .or_else(|| usage.get("input_tokens").and_then(Value::as_u64))
        .unwrap_or(0);
    let output_tokens = usage
        .get("outputTokens")
        .and_then(Value::as_u64)
        .or_else(|| usage.get("output_tokens").and_then(Value::as_u64))
        .unwrap_or(0);

    Some((input_tokens, output_tokens))
}

/// Extracts usage for the active turn, ignoring known delegated-turn payloads.
pub(super) fn extract_turn_usage_for_turn(
    response_value: &Value,
    expected_turn_id: Option<&str>,
) -> Option<(u64, u64)> {
    if let Some(expected_turn_id) = expected_turn_id {
        let turn_id = response_value
            .get("params")
            .and_then(|params| params.get("turn"))
            .and_then(|turn| turn.get("id"))
            .and_then(Value::as_str);
        if turn_id.is_some_and(|payload_turn_id| payload_turn_id != expected_turn_id) {
            return None;
        }
    }

    extract_turn_usage(response_value)
}

/// Extracts per-turn usage from `thread/tokenUsage/updated` notifications.
pub(super) fn extract_thread_token_usage_for_turn(
    response_value: &Value,
    expected_turn_id: Option<&str>,
) -> Option<(u64, u64)> {
    let method = response_value.get("method").and_then(Value::as_str)?;
    if method != "thread/tokenUsage/updated" && method != "thread/token_usage/updated" {
        return None;
    }

    let params = response_value.get("params")?;
    if let Some(expected_turn_id) = expected_turn_id {
        let payload_turn_id = params
            .get("turnId")
            .and_then(Value::as_str)
            .or_else(|| params.get("turn_id").and_then(Value::as_str));
        if payload_turn_id.is_some_and(|turn_id| turn_id != expected_turn_id) {
            return None;
        }
    }

    let token_usage = params
        .get("tokenUsage")
        .or_else(|| params.get("token_usage"))?;
    let breakdown = token_usage
        .get("last")
        .or_else(|| token_usage.get("last_token_usage"))
        .or_else(|| token_usage.get("total"))
        .or_else(|| token_usage.get("total_token_usage"))?;

    let input_tokens = breakdown
        .get("inputTokens")
        .and_then(Value::as_u64)
        .or_else(|| breakdown.get("input_tokens").and_then(Value::as_u64))
        .unwrap_or(0);
    let output_tokens = breakdown
        .get("outputTokens")
        .and_then(Value::as_u64)
        .or_else(|| breakdown.get("output_tokens").and_then(Value::as_u64))
        .unwrap_or(0);

    Some((input_tokens, output_tokens))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_thread_token_usage_for_turn_reads_snake_case_total_usage_shape() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "thread/token_usage/updated",
            "params": {
                "turn_id": "active-turn",
                "token_usage": {
                    "total_token_usage": {
                        "input_tokens": 21,
                        "output_tokens": 8
                    }
                }
            }
        });

        // Act
        let usage = extract_thread_token_usage_for_turn(&response_value, Some("active-turn"));

        // Assert
        assert_eq!(usage, Some((21, 8)));
    }

    #[test]
    fn extract_rate_limits_reads_account_update_windows() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "account/rateLimits/updated",
            "params": {
                "rateLimits": {
                    "primary": {
                        "usedPercent": 23.4,
                        "windowDurationMins": 300,
                        "resetsAt": 1_700_000_000
                    },
                }
            }
        });

        // Act
        let rate_limits = extract_rate_limits(&response_value);

        // Assert
        assert_eq!(
            rate_limits,
            Some(CodexRateLimits {
                primary: Some(RateLimitWindow {
                    resets_at: Some(1_700_000_000),
                    used_percent: 23,
                    window_duration_mins: Some(300),
                }),
                secondary: None,
            })
        );
    }

    #[test]
    fn extract_rate_limits_reads_response_and_snake_case_fields() {
        // Arrange
        let response_value = serde_json::json!({
            "result": {
                "rateLimits": {
                    "primary": null,
                    "secondary": {
                        "used_percent": 50,
                        "window_duration_mins": 10080,
                        "resets_at": 1_800_000_000
                    }
                }
            }
        });

        // Act
        let rate_limits = extract_rate_limits(&response_value);

        // Assert
        assert_eq!(
            rate_limits,
            Some(CodexRateLimits {
                primary: None,
                secondary: Some(RateLimitWindow {
                    resets_at: Some(1_800_000_000),
                    used_percent: 50,
                    window_duration_mins: Some(10080),
                }),
            })
        );
    }

    #[test]
    fn extract_rate_limits_ignores_snapshots_without_windows() {
        // Arrange
        let response_value = serde_json::json!({
            "result": {
                "rateLimits": {
                    "primary": null,
                    "secondary": null
                }
            }
        });

        // Act
        let rate_limits = extract_rate_limits(&response_value);

        // Assert
        assert_eq!(rate_limits, None);
    }

    #[test]
    fn update_turn_usage_from_response_prefers_thread_token_usage_updates() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "thread/tokenUsage/updated",
            "params": {
                "turnId": "active-turn",
                "tokenUsage": {
                    "last": {
                        "inputTokens": 15,
                        "outputTokens": 5
                    }
                }
            }
        });
        let mut completed_turn_usage = None;
        let mut latest_stream_usage = Some((1, 1));

        // Act
        update_turn_usage_from_response(
            &response_value,
            Some("active-turn"),
            &mut completed_turn_usage,
            &mut latest_stream_usage,
        );

        // Assert
        assert_eq!(completed_turn_usage, None);
        assert_eq!(latest_stream_usage, Some((15, 5)));
    }

    #[test]
    fn extract_turn_usage_for_turn_ignores_mismatched_turn_ids() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "turn/completed",
            "params": {
                "turn": {
                    "id": "delegated-turn",
                    "usage": {
                        "inputTokens": 9,
                        "outputTokens": 2
                    }
                }
            }
        });

        // Act
        let usage = extract_turn_usage_for_turn(&response_value, Some("active-turn"));

        // Assert
        assert_eq!(usage, None);
    }
}
