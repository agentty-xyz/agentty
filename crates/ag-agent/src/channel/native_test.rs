//! Native channel unit tests for replayed-history input aging and bootstrap
//! marker encoding.

use ag_harness::TurnRecord;
use serde_json::{Value, json};

use super::{BootstrapMarker, LoadedHistory, input_digest};
use crate::agent::instruction_bootstrap_key;

/// Builds a completed transcript turn at `turn` with `input` blocks.
fn transcript_turn(turn: u64, input: &Value) -> TurnRecord {
    serde_json::from_value(json!({
        "cost": null,
        "error": null,
        "finished_at": 0,
        "input": input,
        "instructions": null,
        "items": [],
        "started_at": 0,
        "status": "completed",
        "turn": turn,
        "usage": null,
    }))
    .expect("transcript turn")
}

/// Returns one text input block of `text`.
fn text(text: &str) -> Value {
    json!([{"type": "text", "text": text}])
}

#[test]
fn loaded_history_ages_the_newest_turn_with_a_single_text_input() {
    // Arrange
    let turns = [
        transcript_turn(0, &text("bootstrap")),
        transcript_turn(1, &json!([])),
        transcript_turn(
            2,
            &json!([{"type": "text", "text": "two"}, {"type": "text", "text": "blocks"}]),
        ),
        transcript_turn(3, &text("follow-up")),
    ];

    // Act
    let history = LoadedHistory::new(true, &turns);

    // Assert
    assert!(history.has_finished_turn);
    assert_eq!(history.age_of(input_digest("bootstrap")), Some(4));
    assert_eq!(history.age_of(input_digest("follow-up")), Some(1));
    assert_eq!(history.age_of(input_digest("two")), None);
    assert_eq!(history.age_of(input_digest("")), None);
}

#[test]
fn bootstrap_marker_round_trips_and_ignores_ids_without_a_digest() {
    // Arrange
    let encoded = BootstrapMarker::encode("session-a", 0xabc).expect("encoded marker");

    // Act
    let parsed = BootstrapMarker::parse(Some(&encoded)).expect("parsed marker");

    // Assert
    assert_eq!(
        Some(parsed.key),
        instruction_bootstrap_key(Some("session-a"))
    );
    assert_eq!(parsed.input_digest, 0xabc);
    assert!(BootstrapMarker::parse(None).is_none());
    assert!(BootstrapMarker::parse(Some("session-a")).is_none());
    assert!(BootstrapMarker::parse(Some("key#not-hex")).is_none());
}
