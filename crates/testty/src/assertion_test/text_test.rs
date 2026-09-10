use crate::assertion::{
    Expected, assert_match_count, assert_not_visible, assert_text_in_region, match_match_count,
    match_not_visible, match_text_in_region,
};
use crate::frame::TerminalFrame;
use crate::region::Region;

#[test]
fn assert_text_in_region_passes_when_found() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Hello World");
    let region = Region::new(0, 0, 80, 1);

    // Act / Assert — should not panic.
    assert_text_in_region(&frame, "Hello", &region);
}

#[test]
#[should_panic(expected = "not found in region")]
fn assert_text_in_region_panics_when_not_found() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Hello World");
    let region = Region::new(20, 0, 60, 1);

    // Act / Assert — should panic.
    assert_text_in_region(&frame, "Hello", &region);
}

#[test]
fn match_text_in_region_returns_ok_when_found() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Hello World");
    let region = Region::new(0, 0, 80, 1);

    // Act
    let result = match_text_in_region(&frame, "Hello", &region);

    // Assert
    assert!(result.is_ok());
}

#[test]
fn match_text_in_region_returns_structured_failure_when_missing() {
    // Arrange — region excludes the actual match position.
    let frame = TerminalFrame::new(80, 24, b"Hello World");
    let region = Region::new(20, 0, 60, 1);

    // Act
    let failure = match_text_in_region(&frame, "Hello", &region).expect_err("should be Err");

    // Assert
    assert_eq!(failure.region, Some(region));
    assert!(failure.message.contains("not found in region"));
    assert!(failure.frame_excerpt.is_empty() || !failure.frame_excerpt.contains("Hello"));
    assert!(
        matches!(&failure.expected, Expected::TextInRegion { needle, .. } if needle == "Hello"),
        "unexpected expected variant: {:?}",
        failure.expected
    );
    // The needle does appear elsewhere in the frame, so structured spans
    // should record it.
    assert_eq!(failure.matched_spans.len(), 1);
}

#[test]
fn assert_not_visible_passes_when_absent() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Hello World");

    // Act / Assert
    assert_not_visible(&frame, "Goodbye");
}

#[test]
#[should_panic(expected = "NOT be visible")]
fn assert_not_visible_panics_when_present() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Hello World");

    // Act / Assert
    assert_not_visible(&frame, "Hello");
}

#[test]
fn match_not_visible_returns_structured_failure_when_present() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Hello World");

    // Act
    let failure = match_not_visible(&frame, "Hello").expect_err("should be Err");

    // Assert
    assert!(failure.region.is_none());
    assert_eq!(failure.matched_spans.len(), 1);
    assert!(
        matches!(&failure.expected, Expected::NotVisible { needle, .. } if needle == "Hello"),
        "unexpected expected variant: {:?}",
        failure.expected
    );
    // frame_excerpt should contain the actual screen contents so renderers
    // have surrounding context, not just the matched span list.
    assert!(failure.frame_excerpt.contains("Hello World"));
}

#[test]
fn assert_match_count_passes_with_correct_count() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"foo bar foo");

    // Act / Assert
    assert_match_count(&frame, "foo", 2);
}

#[test]
#[should_panic(expected = "appear 1 time(s)")]
fn assert_match_count_panics_with_wrong_count() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"foo bar foo");

    // Act / Assert
    assert_match_count(&frame, "foo", 1);
}

#[test]
fn match_match_count_returns_ok_for_correct_count() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"foo bar foo");

    // Act
    let result = match_match_count(&frame, "foo", 2);

    // Assert
    assert!(result.is_ok());
}

#[test]
fn match_match_count_returns_failure_for_wrong_count() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"foo bar foo");

    // Act
    let failure = match_match_count(&frame, "foo", 1).expect_err("should be Err");

    // Assert
    assert!(
        matches!(
            &failure.expected,
            Expected::MatchCount { needle, count, .. } if needle == "foo" && *count == 1
        ),
        "unexpected expected variant: {:?}",
        failure.expected
    );
    assert_eq!(failure.matched_spans.len(), 2);
}
