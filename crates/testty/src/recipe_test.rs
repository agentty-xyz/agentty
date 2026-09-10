use crate::frame::TerminalFrame;
use crate::recipe::{
    expect_instruction_visible, expect_not_visible, expect_selected_tab, expect_status_message,
    expect_unselected_tab, match_dialog_title, match_footer_action, match_instruction_visible,
    match_keybinding_hint, match_not_visible, match_selected_tab, match_status_message,
    match_unselected_tab,
};
use crate::region::Region;

#[test]
fn expect_selected_tab_passes_for_bold_tab() {
    // Arrange — bold "Projects" in the first row.
    let frame = TerminalFrame::new(80, 24, b"\x1b[1mProjects\x1b[0m  Sessions");

    // Act / Assert
    expect_selected_tab(&frame, "Projects");
}

#[test]
#[should_panic(expected = "is not highlighted")]
fn expect_selected_tab_panics_when_not_highlighted() {
    // Arrange — plain text.
    let frame = TerminalFrame::new(80, 24, b"Projects  Sessions");

    // Act / Assert
    expect_selected_tab(&frame, "Projects");
}

#[test]
fn expect_unselected_tab_passes_for_plain_tab() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"\x1b[1mProjects\x1b[0m  Sessions");

    // Act / Assert
    expect_unselected_tab(&frame, "Sessions");
}

#[test]
fn expect_instruction_visible_finds_text() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"\r\n\r\nPress Enter to continue");

    // Act / Assert
    expect_instruction_visible(&frame, "Press Enter");
}

#[test]
fn expect_not_visible_passes_when_absent() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Hello World");

    // Act / Assert
    expect_not_visible(&frame, "Goodbye");
}

#[test]
fn expect_status_message_finds_anywhere() {
    // Arrange
    let mut data = Vec::new();
    for _ in 0..10 {
        data.extend_from_slice(b"\r\n");
    }
    data.extend_from_slice(b"Status: OK");
    let frame = TerminalFrame::new(80, 24, &data);

    // Act / Assert
    expect_status_message(&frame, "Status: OK");
}

#[test]
fn match_selected_tab_returns_ok_for_bold_tab() {
    // Arrange — bold "Projects" in the first row.
    let frame = TerminalFrame::new(80, 24, b"\x1b[1mProjects\x1b[0m  Sessions");

    // Act
    let result = match_selected_tab(&frame, "Projects");

    // Assert
    assert!(result.is_ok());
}

#[test]
fn match_selected_tab_returns_failure_when_not_highlighted() {
    // Arrange — plain text in the header.
    let frame = TerminalFrame::new(80, 24, b"Projects  Sessions");

    // Act
    let failure = match_selected_tab(&frame, "Projects").expect_err("should be Err");

    // Assert — composition surfaces the highlight check failure.
    assert!(matches!(
        &failure.expected,
        crate::assertion::Expected::Highlighted { needle, .. } if needle == "Projects"
    ));
}

#[test]
fn match_selected_tab_returns_failure_when_label_missing_from_header() {
    // Arrange — label not present in the top row.
    let frame = TerminalFrame::new(80, 24, b"Other  Tabs");

    // Act
    let failure = match_selected_tab(&frame, "Projects").expect_err("should be Err");

    // Assert — composition stops at the region check before reaching
    // highlight.
    assert!(matches!(
        &failure.expected,
        crate::assertion::Expected::TextInRegion { needle, .. } if needle == "Projects"
    ));
}

#[test]
fn match_unselected_tab_returns_ok_for_plain_tab() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"\x1b[1mProjects\x1b[0m  Sessions");

    // Act
    let result = match_unselected_tab(&frame, "Sessions");

    // Assert
    assert!(result.is_ok());
}

#[test]
fn match_unselected_tab_returns_failure_when_highlighted() {
    // Arrange — bold tab fails the unselected check.
    let frame = TerminalFrame::new(80, 24, b"\x1b[1mProjects\x1b[0m  Sessions");

    // Act
    let failure = match_unselected_tab(&frame, "Projects").expect_err("should be Err");

    // Assert
    assert!(matches!(
        &failure.expected,
        crate::assertion::Expected::NotHighlighted { needle, .. } if needle == "Projects"
    ));
}

#[test]
fn match_instruction_visible_returns_failure_when_absent() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Hello");

    // Act
    let failure = match_instruction_visible(&frame, "Press Enter").expect_err("should be Err");

    // Assert
    assert!(matches!(
        &failure.expected,
        crate::assertion::Expected::TextInRegion { needle, .. } if needle == "Press Enter"
    ));
}

#[test]
fn match_keybinding_hint_returns_ok_when_in_footer() {
    // Arrange — write "Tab" on the last row.
    let mut data = Vec::new();
    for _ in 0..23 {
        data.extend_from_slice(b"\r\n");
    }
    data.extend_from_slice(b"Tab Enter q");
    let frame = TerminalFrame::new(80, 24, &data);

    // Act
    let result = match_keybinding_hint(&frame, "Tab");

    // Assert
    assert!(result.is_ok());
}

#[test]
fn match_footer_action_returns_failure_when_missing() {
    // Arrange — empty footer.
    let frame = TerminalFrame::new(80, 24, b"Header text");

    // Act
    let failure = match_footer_action(&frame, "Quit").expect_err("should be Err");

    // Assert
    assert!(matches!(
        &failure.expected,
        crate::assertion::Expected::TextInRegion { needle, .. } if needle == "Quit"
    ));
}

#[test]
fn match_dialog_title_returns_ok_when_in_upper_region() {
    // Arrange — title near the top of the frame.
    let frame = TerminalFrame::new(80, 24, b"\r\n\r\nConfirm Delete");

    // Act
    let result = match_dialog_title(&frame, "Confirm Delete");

    // Assert
    assert!(result.is_ok());
}

#[test]
fn match_status_message_returns_ok_when_present() {
    // Arrange
    let mut data = Vec::new();
    for _ in 0..10 {
        data.extend_from_slice(b"\r\n");
    }
    data.extend_from_slice(b"Status: OK");
    let frame = TerminalFrame::new(80, 24, &data);

    // Act
    let result = match_status_message(&frame, "Status: OK");

    // Assert
    assert!(result.is_ok());
}

#[test]
fn match_not_visible_returns_failure_when_present() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Hello World");

    // Act
    let failure = match_not_visible(&frame, "Hello").expect_err("should be Err");

    // Assert
    assert!(matches!(
        &failure.expected,
        crate::assertion::Expected::NotVisible { needle, .. } if needle == "Hello"
    ));
}

#[test]
fn match_selected_tab_inspects_header_occurrence_when_label_repeats_in_body() {
    // Arrange — bold "Projects" tab in the header row, plus a plain
    // (unhighlighted) "Projects" later in the body. The recipe must
    // judge the header occurrence specifically.
    let mut data: Vec<u8> = Vec::new();
    data.extend_from_slice(b"\x1b[1mProjects\x1b[0m  Sessions");
    for _ in 0..6 {
        data.extend_from_slice(b"\r\n");
    }
    data.extend_from_slice(b"Projects mentioned here is plain body text");
    let frame = TerminalFrame::new(80, 24, &data);

    // Act
    let result = match_selected_tab(&frame, "Projects");

    // Assert
    assert!(result.is_ok());
}

#[test]
fn match_unselected_tab_inspects_header_occurrence_when_label_repeats_in_body() {
    // Arrange — plain "Sessions" tab in the header row, plus a bold
    // "Sessions" later in the body. The recipe must judge the header
    // occurrence specifically rather than the highlighted body match.
    let mut data: Vec<u8> = Vec::new();
    data.extend_from_slice(b"Projects  Sessions");
    for _ in 0..6 {
        data.extend_from_slice(b"\r\n");
    }
    data.extend_from_slice(b"\x1b[1mSessions\x1b[0m heading");
    let frame = TerminalFrame::new(80, 24, &data);

    // Act
    let result = match_unselected_tab(&frame, "Sessions");

    // Assert
    assert!(result.is_ok());
}

#[test]
fn match_selected_tab_failure_is_scoped_to_header_region() {
    // Arrange — plain "Projects" in the header is the occurrence the
    // recipe must judge, even though a bold "Projects" appears later
    // in the body.
    let mut data: Vec<u8> = Vec::new();
    data.extend_from_slice(b"Projects  Sessions");
    for _ in 0..6 {
        data.extend_from_slice(b"\r\n");
    }
    data.extend_from_slice(b"\x1b[1mProjects\x1b[0m elsewhere");
    let frame = TerminalFrame::new(80, 24, &data);

    // Act
    let failure = match_selected_tab(&frame, "Projects").expect_err("should be Err");

    // Assert — failure carries the header region and the matched span
    // sits in row 0, proving the check is scoped to the header.
    let header = Region::top_row(frame.cols());
    assert_eq!(failure.region, Some(header));
    assert_eq!(failure.matched_spans.len(), 1);
    assert_eq!(failure.matched_spans[0].rect.row, 0);
}

#[test]
fn match_unselected_tab_failure_is_scoped_to_header_region() {
    // Arrange — bold "Sessions" in the header is the occurrence the
    // recipe must reject, even though a plain "Sessions" appears
    // later in the body.
    let mut data: Vec<u8> = Vec::new();
    data.extend_from_slice(b"Projects  \x1b[1mSessions\x1b[0m");
    for _ in 0..6 {
        data.extend_from_slice(b"\r\n");
    }
    data.extend_from_slice(b"Sessions elsewhere");
    let frame = TerminalFrame::new(80, 24, &data);

    // Act
    let failure = match_unselected_tab(&frame, "Sessions").expect_err("should be Err");

    // Assert — failure carries the header region and the matched span
    // sits in row 0, proving the check is scoped to the header.
    let header = Region::top_row(frame.cols());
    assert_eq!(failure.region, Some(header));
    assert_eq!(failure.matched_spans.len(), 1);
    assert_eq!(failure.matched_spans[0].rect.row, 0);
}

#[test]
fn match_recipes_compose_with_soft_assertions() {
    // Arrange — frame missing both the expected tab and the footer hint.
    let frame = TerminalFrame::new(80, 24, b"Other Header");

    // Act — accumulate every failure across composed recipes without
    // failing fast, then drain to suppress the drop-time panic.
    let mut soft = crate::assertion::SoftAssertions::new();
    soft.check(match_selected_tab(&frame, "Projects"));
    soft.check(match_keybinding_hint(&frame, "Quit"));
    let failures = soft.into_failures();

    // Assert — both recipe failures land in the accumulator.
    assert_eq!(failures.len(), 2);
}
