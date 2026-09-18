//! Public review status contract used by persistence and orchestration.
use ag_session::FocusedReviewStatus;

#[test]
fn partial_coverage_round_trips_without_becoming_a_clean_review() {
    // Arrange
    let text =
        "## Review\n### Suggestions\n- None\n### Coverage\n\nPartial review: unfinished batches";
    // Act
    let status = FocusedReviewStatus::for_text(text);
    let restored = status
        .to_string()
        .parse::<FocusedReviewStatus>()
        .expect("persisted status");
    // Assert
    assert_eq!(restored, FocusedReviewStatus::Partial);
}
