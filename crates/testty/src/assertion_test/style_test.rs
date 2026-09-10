use crate::assertion::{
    Expected, assert_span_is_highlighted, assert_span_is_not_highlighted, assert_text_has_fg_color,
    match_span_is_highlighted, match_span_is_not_highlighted, match_text_has_bg_color,
    match_text_has_fg_color,
};
use crate::frame::{CellColor, TerminalFrame};

#[test]
fn assert_span_is_highlighted_detects_bold() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"\x1b[1mBold\x1b[0m");

    // Act / Assert
    assert_span_is_highlighted(&frame, "Bold");
}

#[test]
fn match_span_is_highlighted_returns_failure_when_missing() {
    // Arrange — text is not on screen.
    let frame = TerminalFrame::new(80, 24, b"plain text");

    // Act
    let failure = match_span_is_highlighted(&frame, "missing").expect_err("should be Err");

    // Assert
    assert!(failure.matched_spans.is_empty());
    assert!(
        matches!(&failure.expected, Expected::Highlighted { needle, .. } if needle == "missing"),
        "unexpected expected variant: {:?}",
        failure.expected
    );
    // frame_excerpt should include the visible screen contents even on
    // missing-text failures.
    assert!(failure.frame_excerpt.contains("plain text"));
}

#[test]
fn match_span_is_highlighted_returns_failure_for_plain_text() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"plain text");

    // Act
    let failure = match_span_is_highlighted(&frame, "plain").expect_err("should be Err");

    // Assert
    assert!(
        matches!(&failure.expected, Expected::Highlighted { needle, .. } if needle == "plain"),
        "unexpected expected variant: {:?}",
        failure.expected
    );
    assert_eq!(failure.matched_spans.len(), 1);
}

#[test]
fn assert_span_is_not_highlighted_for_plain_text() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"plain text");

    // Act / Assert
    assert_span_is_not_highlighted(&frame, "plain");
}

#[test]
fn match_span_is_not_highlighted_returns_failure_for_bold() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"\x1b[1mBold\x1b[0m");

    // Act
    let failure = match_span_is_not_highlighted(&frame, "Bold").expect_err("should be Err");

    // Assert
    assert!(
        matches!(&failure.expected, Expected::NotHighlighted { needle, .. } if needle == "Bold"),
        "unexpected expected variant: {:?}",
        failure.expected
    );
}

#[test]
fn assert_text_has_fg_color_passes() {
    // Arrange — ANSI red foreground.
    let frame = TerminalFrame::new(80, 24, b"\x1b[31mRed\x1b[0m");

    // Act / Assert
    assert_text_has_fg_color(&frame, "Red", &CellColor::new(128, 0, 0));
}

#[test]
#[should_panic(expected = "foreground")]
fn assert_text_has_fg_color_panics_on_mismatch() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"\x1b[31mRed\x1b[0m");

    // Act / Assert
    assert_text_has_fg_color(&frame, "Red", &CellColor::new(0, 255, 0));
}

#[test]
fn match_text_has_fg_color_returns_failure_when_text_missing() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"plain");

    // Act
    let failure = match_text_has_fg_color(&frame, "missing", &CellColor::new(0, 255, 0))
        .expect_err("should be Err");

    // Assert
    assert!(failure.message.contains("not found in frame"));
    assert!(failure.matched_spans.is_empty());
    // frame_excerpt should show what is on screen so renderers can surface
    // surrounding context for the missing-text failure.
    assert!(failure.frame_excerpt.contains("plain"));
}

#[test]
fn match_text_has_fg_color_returns_failure_on_mismatch() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"\x1b[31mRed\x1b[0m");

    // Act
    let failure = match_text_has_fg_color(&frame, "Red", &CellColor::new(0, 255, 0))
        .expect_err("should be Err");

    // Assert
    assert!(
        matches!(
            &failure.expected,
            Expected::ForegroundColor { needle, color, .. }
                if needle == "Red" && *color == CellColor::new(0, 255, 0)
        ),
        "unexpected expected variant: {:?}",
        failure.expected
    );
    assert_eq!(failure.matched_spans.len(), 1);
}

#[test]
fn match_text_has_bg_color_returns_failure_when_text_missing() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"plain");

    // Act
    let failure = match_text_has_bg_color(&frame, "missing", &CellColor::new(0, 0, 0))
        .expect_err("should be Err");

    // Assert
    assert!(failure.message.contains("not found in frame"));
    assert!(failure.matched_spans.is_empty());
    // frame_excerpt should show what is on screen so renderers can surface
    // surrounding context for the missing-text failure.
    assert!(failure.frame_excerpt.contains("plain"));
}

#[test]
fn match_text_has_bg_color_returns_ok_when_match() {
    // Arrange — ANSI red background (index 41 → CellColor 128,0,0).
    let frame = TerminalFrame::new(80, 24, b"\x1b[41mActive\x1b[0m");

    // Act
    let result = match_text_has_bg_color(&frame, "Active", &CellColor::new(128, 0, 0));

    // Assert
    assert!(result.is_ok());
}

#[test]
fn match_text_has_bg_color_returns_failure_on_mismatch() {
    // Arrange — red background, but caller expects a different color.
    let frame = TerminalFrame::new(80, 24, b"\x1b[41mActive\x1b[0m");

    // Act
    let failure = match_text_has_bg_color(&frame, "Active", &CellColor::new(0, 0, 128))
        .expect_err("should be Err");

    // Assert
    assert!(
        matches!(
            &failure.expected,
            Expected::BackgroundColor { needle, color, .. }
                if needle == "Active" && *color == CellColor::new(0, 0, 128)
        ),
        "unexpected expected variant: {:?}",
        failure.expected
    );
    assert_eq!(failure.matched_spans.len(), 1);
    // frame_excerpt should reflect on-screen content so renderers can show
    // the surrounding context for the mismatch.
    assert!(failure.frame_excerpt.contains("Active"));
}

#[test]
fn match_span_is_not_highlighted_returns_failure_when_missing() {
    // Arrange — text is not on screen at all.
    let frame = TerminalFrame::new(80, 24, b"plain text");

    // Act
    let failure = match_span_is_not_highlighted(&frame, "missing").expect_err("should be Err");

    // Assert
    assert!(failure.matched_spans.is_empty());
    assert!(failure.message.contains("not found in frame"));
    assert!(
        matches!(&failure.expected, Expected::NotHighlighted { needle, .. } if needle == "missing"),
        "unexpected expected variant: {:?}",
        failure.expected
    );
    // frame_excerpt should include the visible screen contents even on
    // missing-text failures.
    assert!(failure.frame_excerpt.contains("plain text"));
}
