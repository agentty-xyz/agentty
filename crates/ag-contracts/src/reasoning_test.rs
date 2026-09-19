use crate::ReasoningLevel;

#[test]
/// Ensures reasoning-level parsing accepts all supported persisted values.
fn test_reasoning_level_from_str_parses_supported_values() {
    // Arrange

    // Act
    let low_level = "low".parse::<ReasoningLevel>();
    let medium_level = "medium".parse::<ReasoningLevel>();
    let high_level = "high".parse::<ReasoningLevel>();
    let xhigh_level = "xhigh".parse::<ReasoningLevel>();
    let max_level = "max".parse::<ReasoningLevel>();

    // Assert
    assert_eq!(low_level, Ok(ReasoningLevel::Low));
    assert_eq!(medium_level, Ok(ReasoningLevel::Medium));
    assert_eq!(high_level, Ok(ReasoningLevel::High));
    assert_eq!(xhigh_level, Ok(ReasoningLevel::XHigh));
    assert_eq!(max_level, Ok(ReasoningLevel::Max));
}

#[test]
/// Ensures unsupported reasoning values return a parse error.
fn test_reasoning_level_from_str_rejects_unknown_values() {
    // Arrange

    // Act
    let parse_result = "minimal".parse::<ReasoningLevel>();

    // Assert
    assert!(parse_result.is_err());
}

#[test]
/// Ensures persisted reasoning identifiers stay stable even if provider
/// transport names change in the future.
fn test_reasoning_level_as_str_returns_stable_persisted_values() {
    // Arrange / Act / Assert
    assert_eq!(ReasoningLevel::Low.as_str(), "low");
    assert_eq!(ReasoningLevel::Medium.as_str(), "medium");
    assert_eq!(ReasoningLevel::High.as_str(), "high");
    assert_eq!(ReasoningLevel::XHigh.as_str(), "xhigh");
    assert_eq!(ReasoningLevel::Max.as_str(), "max");
}
