use ag_harness::model::ModelMessage;
use ag_harness::store::HistoryTurn;

use super::{BootstrapMarker, LoadedHistory, input_digest};
use crate::agent::instruction_bootstrap_key;

/// Builds a completed history turn from `messages`.
fn history_turn(messages: Vec<ModelMessage>) -> HistoryTurn {
    HistoryTurn {
        messages,
        stop: None,
    }
}

#[test]
fn loaded_history_ages_the_newest_turn_with_a_matching_input() {
    // Arrange
    let turns = [
        history_turn(vec![ModelMessage::User("bootstrap".to_string())]),
        history_turn(vec![ModelMessage::Assistant("no input".to_string())]),
        history_turn(vec![ModelMessage::User("follow-up".to_string())]),
    ];

    // Act
    let history = LoadedHistory::new(true, &turns);

    // Assert
    assert!(history.has_finished_turn);
    assert_eq!(history.age_of(input_digest("bootstrap")), Some(3));
    assert_eq!(history.age_of(input_digest("follow-up")), Some(1));
    assert_eq!(history.age_of(input_digest("no input")), None);
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
