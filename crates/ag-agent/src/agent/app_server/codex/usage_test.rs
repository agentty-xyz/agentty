use crate::agent::app_server::codex::usage::{
    extract_thread_token_usage_for_turn, extract_turn_usage_for_turn,
    update_turn_usage_from_response,
};

#[test]
fn extract_thread_token_usage_for_turn_does_not_treat_snake_case_totals_as_request_usage() {
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
    assert_eq!(usage, None);
}

#[test]
fn cumulative_only_updates_clear_old_request_fallbacks_but_ignore_delegated_turns() {
    // Arrange
    let response = serde_json::json!({"method":"thread/tokenUsage/updated","params":{"turnId":"active-turn","tokenUsage":{"total":{"inputTokens":50_000,"outputTokens":1_000}}}});
    let mut completed = Some((9, 1));
    let mut latest = Some((50, 5));
    // Act
    update_turn_usage_from_response(&response, Some("other-turn"), &mut completed, &mut latest);
    let delegated = latest;
    update_turn_usage_from_response(&response, Some("active-turn"), &mut completed, &mut latest);
    // Assert
    assert_eq!(delegated, Some((50, 5)));
    assert_eq!(latest, None);
    assert_eq!(completed, Some((9, 1)));
}

#[test]
fn snake_case_last_usage_is_safe_for_request_fallback_accounting() {
    // Arrange
    let response = serde_json::json!({"method":"thread/token_usage/updated","params":{"turn_id":"active-turn","token_usage":{"total_token_usage":{"input_tokens":50_000,"output_tokens":1_000},"last_token_usage":{"input_tokens":50,"output_tokens":5}}}});
    // Act
    let usage = extract_thread_token_usage_for_turn(&response, Some("active-turn"));
    // Assert
    assert_eq!(usage, Some((50, 5)));
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

#[test]
fn cumulative_usage_counts_every_request_once_and_retains_turn_baselines() {
    // Arrange
    let mut tracker = super::UsageTracker::default();
    tracker.begin_turn();
    let first = token_update(100, 10, Some(30), Some(100));
    let second = token_update(250, 30, Some(70), Some(150));
    // Act
    tracker.observe(&first, Some("active-turn"));
    tracker.observe(&second, Some("active-turn"));
    tracker.observe(&second, Some("active-turn"));
    // Assert
    assert_eq!(tracker.totals(), Some((250, 30)));
    assert_eq!(tracker.turn_cached, Some(70));
    assert_eq!(tracker.latest_input, Some(150));
    // Act
    tracker.begin_turn();
    tracker.observe(
        &token_update(430, 50, Some(90), Some(180)),
        Some("active-turn"),
    );
    // Assert
    assert_eq!(tracker.totals(), Some((180, 20)));
    assert_eq!(tracker.turn_cached, Some(20));
    assert_eq!(tracker.latest_input, Some(180));
}

#[test]
fn resumed_threads_bill_every_response_after_the_reconstructed_baseline() {
    // Arrange
    let mut tracker = super::UsageTracker::default();
    tracker.resumed();
    tracker.begin_turn();
    // Act
    tracker.observe(
        &response_update((50_000, 1_000, 900), (100, 10, 30)),
        Some("active-turn"),
    );
    tracker.observe(
        &response_update((50_300, 1_030, 960), (300, 30, 60)),
        Some("active-turn"),
    );
    // Assert
    assert_eq!(tracker.totals(), Some((400, 40)));
    assert_eq!(tracker.turn_cached, Some(90));
    assert_eq!(tracker.latest_input, Some(300));
    // Act
    tracker.begin_turn();
    tracker.observe(&token_update(20, 5, Some(10), Some(20)), None);
    // Assert
    assert_eq!(tracker.totals(), Some((20, 5)));
    assert_eq!(tracker.turn_cached, Some(10));
}

#[test]
fn resumed_baselines_reconstruct_only_counters_consistent_with_the_latest_response() {
    // Arrange
    let cases = [
        (token_update(50_000, 1_000, Some(900), None), None, None),
        (
            response_update((50, 1_000, 900), (100, 10, 30)),
            None,
            Some(30),
        ),
        (
            serde_json::json!({"method":"thread/token_usage/updated","params":{"token_usage":{"total_token_usage":{"input_tokens":500,"output_tokens":50,"cached_input_tokens":20},"last_token_usage":{"input_tokens":100,"output_tokens":10,"cached_input_tokens":40}}}}),
            Some((100, 10)),
            None,
        ),
    ];

    // Act / Assert
    for (update, expected_totals, expected_cached) in cases {
        let mut tracker = super::UsageTracker::default();
        tracker.resumed();
        tracker.begin_turn();
        tracker.observe(&update, None);
        assert_eq!(tracker.totals(), expected_totals);
        assert_eq!(tracker.turn_cached, expected_cached);
    }
}

#[test]
fn unknown_fields_and_last_only_notifications_remain_unknown() {
    // Arrange
    let mut tracker = super::UsageTracker::default();
    // Act / Assert
    assert_eq!(tracker.totals(), None);
    for usage in [
        serde_json::json!({"last":{"inputTokens":12,"outputTokens":3}}),
        serde_json::json!({"total":{"inputTokens":12},"last":{}}),
        serde_json::json!({"total":{"inputTokens":12,"outputTokens":3}}),
    ] {
        tracker.begin_turn();
        tracker.observe(&serde_json::json!({"method":"thread/tokenUsage/updated","params":{"tokenUsage":usage}}), None);
        assert_eq!(tracker.turn_cached, None);
        if usage.get("last").is_some() {
            assert_eq!(tracker.totals(), None);
        } else {
            assert_eq!(tracker.latest_input, None);
        }
    }
}

#[test]
fn delegated_usage_is_ignored_and_compaction_invalidates_context_size() {
    // Arrange
    let mut tracker = super::UsageTracker::default();
    tracker.begin_turn();
    let snake_case = serde_json::json!({"method":"thread/token_usage/updated","params":{"turn_id":"active-turn","token_usage":{"last_token_usage":{"input_tokens":80,"output_tokens":2},"total_token_usage":{"input_tokens":80,"output_tokens":2,"cached_input_tokens":40}}}});
    // Act
    tracker.observe(&snake_case, Some("active-turn"));
    tracker.observe(
        &token_update(400, 40, Some(300), Some(320)),
        Some("other-turn"),
    );
    // Assert
    assert_eq!(tracker.totals(), Some((80, 2)));
    assert_eq!(tracker.turn_cached, Some(40));
    assert_eq!(tracker.latest_input, Some(80));
    // Act
    tracker.observe(&serde_json::json!({"method":"item/completed","params":{"item":{"type":"contextCompaction"}}}), None);
    // Assert
    assert_eq!(tracker.latest_input, None);
    assert_eq!(tracker.totals(), Some((80, 2)));
    // Act
    tracker.observe(&serde_json::json!({"method":"turn/completed","params":{"turn":{"usage":{"inputTokens":7,"outputTokens":1}}}}), None);
    // Assert
    assert_eq!(tracker.latest_input, None);
}

fn token_update(
    input: u64,
    output: u64,
    cached: Option<u64>,
    last_input: Option<u64>,
) -> serde_json::Value {
    let mut total = serde_json::json!({"inputTokens":input,"outputTokens":output});
    if let Some(cached) = cached {
        total["cachedInputTokens"] = cached.into();
    }
    let mut usage = serde_json::json!({"total":total});
    if let Some(input) = last_input {
        usage["last"] = serde_json::json!({"inputTokens":input,"outputTokens":output});
    }
    serde_json::json!({"method":"thread/tokenUsage/updated","params":{"turnId":"active-turn","tokenUsage":usage}})
}

/// Builds one snapshot whose input, output, and cached counters list the
/// thread total followed by the latest response.
fn response_update(total: (u64, u64, u64), last: (u64, u64, u64)) -> serde_json::Value {
    let breakdown = |(input, output, cached): (u64, u64, u64)| serde_json::json!({"inputTokens":input,"outputTokens":output,"cachedInputTokens":cached});

    serde_json::json!({"method":"thread/tokenUsage/updated","params":{"turnId":"active-turn","tokenUsage":{"total":breakdown(total),"last":breakdown(last)}}})
}

#[test]
fn missing_cumulative_turns_discard_old_baselines_and_recover_from_a_fresh_snapshot() {
    // Arrange / Act / Assert
    for missing in [
        serde_json::json!({"method":"thread/tokenUsage/updated","params":{"tokenUsage":{"last":{"inputTokens":50,"outputTokens":5}}}}),
        serde_json::json!({"method":"turn/completed","params":{"turn":{"usage":{"inputTokens":50,"outputTokens":5}}}}),
        serde_json::json!({"method":"thread/tokenUsage/updated","params":{"tokenUsage":{"total":{"inputTokens":150,"cachedInputTokens":45},"last":{"inputTokens":50,"outputTokens":5}}}}),
    ] {
        let mut tracker = super::UsageTracker::default();
        tracker.begin_turn();
        tracker.observe(&token_update(100, 10, Some(30), Some(100)), None);
        tracker.finish_turn();
        assert_eq!(tracker.totals(), Some((100, 10)));
        tracker.begin_turn();
        tracker.observe(&missing, None);
        tracker.finish_turn();
        assert_eq!(tracker.totals(), None);
        assert_eq!(tracker.previous, None);
        assert_eq!(tracker.previous_cached, None);
        tracker.begin_turn();
        tracker.observe(&token_update(200, 20, Some(60), Some(50)), None);
        tracker.finish_turn();
        assert_eq!(tracker.totals(), Some((50, 20)));
        assert_eq!(tracker.previous, Some((200, 20)));
        assert_eq!(tracker.previous_cached, Some(60));
        tracker.begin_turn();
        tracker.observe(&token_update(250, 25, Some(75), Some(50)), None);
        tracker.finish_turn();
        assert_eq!(tracker.totals(), Some((50, 5)));
        assert_eq!(tracker.turn_cached, Some(15));
    }
}
