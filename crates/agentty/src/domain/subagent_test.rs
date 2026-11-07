use std::num::NonZeroUsize;

use crate::domain::subagent::SubagentLimit;

#[test]
fn test_limits_round_trip_through_persisted_values() {
    // Arrange
    let expected = [
        (SubagentLimit::One, 1),
        (SubagentLimit::Two, 2),
        (SubagentLimit::Four, 4),
        (SubagentLimit::Eight, 8),
    ];

    // Act & Assert
    for (limit, count) in expected {
        assert_eq!(
            limit.limit(),
            NonZeroUsize::new(count).expect("count is non-zero")
        );
        assert_eq!(SubagentLimit::parse_persisted(limit.as_str()), Some(limit));
    }
}

#[test]
fn test_unknown_values_are_rejected_and_default_is_two() {
    // Arrange
    let unknown = ["0", "3", "", "two"];

    // Act & Assert
    for value in unknown {
        assert_eq!(SubagentLimit::parse_persisted(value), None);
    }
    assert_eq!(SubagentLimit::default(), SubagentLimit::Two);
}
