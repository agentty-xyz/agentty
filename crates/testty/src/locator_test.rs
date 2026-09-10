use crate::frame::{CellColor, CellStyle};
use crate::locator::MatchedSpan;
use crate::region::Region;

#[test]
fn has_fg_matches_exact_color() {
    // Arrange
    let span = sample_span();

    // Act / Assert
    assert!(span.has_fg(&CellColor::white()));
    assert!(!span.has_fg(&CellColor::black()));
}

#[test]
fn has_bg_matches_exact_color() {
    // Arrange
    let span = sample_span();

    // Act / Assert
    assert!(span.has_bg(&CellColor::new(0, 0, 128)));
    assert!(!span.has_bg(&CellColor::white()));
}

#[test]
fn is_highlighted_detects_bold() {
    // Arrange
    let span = sample_span();

    // Act / Assert
    assert!(span.is_highlighted());
    assert!(span.is_bold());
}

#[test]
fn is_highlighted_detects_background_color() {
    // Arrange
    let span = MatchedSpan {
        text: "item".to_string(),
        rect: Region::new(0, 0, 4, 1),
        foreground: None,
        background: Some(CellColor::new(50, 50, 50)),
        style: CellStyle::default(),
    };

    // Act / Assert
    assert!(span.is_highlighted());
}

#[test]
fn not_highlighted_when_plain() {
    // Arrange
    let span = MatchedSpan {
        text: "plain".to_string(),
        rect: Region::new(0, 0, 5, 1),
        foreground: None,
        background: None,
        style: CellStyle::default(),
    };

    // Act / Assert
    assert!(!span.is_highlighted());
}

fn sample_span() -> MatchedSpan {
    MatchedSpan {
        text: "Tab".to_string(),
        rect: Region::new(5, 0, 3, 1),
        foreground: Some(CellColor::white()),
        background: Some(CellColor::new(0, 0, 128)),
        style: CellStyle::from_raw(0x01),
    }
}
