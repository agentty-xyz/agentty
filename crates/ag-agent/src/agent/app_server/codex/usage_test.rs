use crate::agent::app_server::codex::usage::{
    extract_thread_token_usage_for_turn, extract_turn_usage_for_turn,
    update_turn_usage_from_response,
};

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
