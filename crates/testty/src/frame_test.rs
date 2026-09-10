use super::ansi_index_to_rgb;
use crate::frame::{CellColor, CellStyle, TerminalFrame};
use crate::region::Region;

#[test]
fn new_frame_captures_plain_text() {
    // Arrange
    let data = b"Hello, World!";

    // Act
    let frame = TerminalFrame::new(80, 24, data);

    // Assert
    assert_eq!(frame.row_text(0), "Hello, World!");
    assert_eq!(frame.cols(), 80);
    assert_eq!(frame.rows(), 24);
}

#[test]
fn row_text_trims_trailing_spaces() {
    // Arrange
    let data = b"abc";

    // Act
    let frame = TerminalFrame::new(80, 24, data);

    // Assert
    assert_eq!(frame.row_text(0), "abc");
    assert_eq!(frame.row_text(0).len(), 3);
}

#[test]
fn find_text_returns_all_matches() {
    // Arrange
    let data = b"foo bar foo";

    // Act
    let frame = TerminalFrame::new(80, 24, data);
    let matches = frame.find_text("foo");

    // Assert
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0].rect.col, 0);
    assert_eq!(matches[1].rect.col, 8);
}

#[test]
fn find_text_preserves_blank_cells_after_terminal_clear() {
    // Arrange — clear the terminal, then paint separate text runs while
    // leaving untouched cells between them.
    let data = b"\x1b[2J\x1b[1;1HNo\x1b[1;4Hdraft\x1b[1;10Hmessages";
    let frame = TerminalFrame::new(40, 2, data);

    // Act
    let matches = frame.find_text("No draft messages");

    // Assert
    assert_eq!(frame.row_text(0), "No draft messages");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].rect, Region::new(0, 0, 17, 1));
}

#[test]
fn find_text_with_empty_needle_returns_empty() {
    // Arrange
    let data = b"foo bar";
    let frame = TerminalFrame::new(80, 24, data);

    // Act
    let matches = frame.find_text("");

    // Assert
    assert!(matches.is_empty());
}

#[test]
fn find_text_locates_multibyte_utf8_at_correct_column() {
    // Arrange — "café" has a multi-byte é (2 bytes in UTF-8) but each
    // character occupies exactly one terminal column.
    let data = "café ok".as_bytes();
    let frame = TerminalFrame::new(80, 24, data);

    // Act
    let matches = frame.find_text("ok");

    // Assert — "ok" starts at terminal column 5, not byte offset 6.
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].rect.col, 5);
    assert_eq!(matches[0].rect.width, 2);
}

#[test]
fn find_text_locates_text_after_wide_character() {
    // Arrange — the wide glyph occupies two terminal cells, including a
    // continuation cell that must not add text or shift later matches.
    let data = "あ ok".as_bytes();
    let frame = TerminalFrame::new(80, 24, data);

    // Act
    let matches = frame.find_text("ok");

    // Assert
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].rect, Region::new(3, 0, 2, 1));
}

#[test]
fn find_text_in_region_filters_by_region() {
    // Arrange
    let data = b"foo bar foo";
    let frame = TerminalFrame::new(80, 24, data);
    let region = Region::new(5, 0, 75, 1);

    // Act
    let matches = frame.find_text_in_region("foo", &region);

    // Assert
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].rect.col, 8);
}

#[test]
fn text_in_region_extracts_substring() {
    // Arrange
    let data = b"Hello, World!";
    let frame = TerminalFrame::new(80, 24, data);
    let region = Region::new(7, 0, 5, 1);

    // Act
    let text = frame.text_in_region(&region);

    // Assert
    assert_eq!(text, "World");
}

#[test]
fn all_text_joins_rows() {
    // Arrange
    let data = b"Line 1\r\nLine 2\r\nLine 3";

    // Act
    let frame = TerminalFrame::new(80, 24, data);
    let text = frame.all_text();

    // Assert
    assert!(text.contains("Line 1"));
    assert!(text.contains("Line 2"));
    assert!(text.contains("Line 3"));
}

#[test]
fn row_text_preserves_blank_columns_between_runs() {
    // Arrange — write "Hello", jump cursor to column 11, then write
    // "World". Cells 5..10 stay blank between the two non-empty runs.
    let data = b"Hello\x1b[1;11HWorld";

    // Act
    let frame = TerminalFrame::new(80, 24, data);
    let row = frame.row_text(0);
    let all = frame.all_text();

    // Assert — blank columns are preserved as spaces so callers can
    // tell that `Hello` and `World` are not adjacent on the screen.
    assert_eq!(row, "Hello     World");
    assert!(
        all.contains("Hello     World"),
        "all_text should preserve blank columns between runs, got: {all:?}"
    );
    assert!(
        !all.contains("HelloWorld"),
        "blank columns must not collapse distinct runs together, got: {all:?}"
    );
}

#[test]
fn row_text_skips_wide_character_continuation_cells() {
    // Arrange — `あ` is a wide CJK glyph that occupies two grid columns.
    // The continuation cell reports empty contents but must not be
    // padded with a phantom space; the leading cell already covers it.
    let data = "あ_".as_bytes();

    // Act
    let frame = TerminalFrame::new(80, 24, data);
    let row = frame.row_text(0);

    // Assert — the wide glyph and the trailing underscore stay
    // contiguous, with no phantom space inserted by the continuation
    // cell.
    assert_eq!(row, "あ_");
}

#[test]
fn row_text_trims_trailing_empty_cells() {
    // Arrange — short content followed by many empty cells.
    let data = b"abc";

    // Act
    let frame = TerminalFrame::new(80, 24, data);

    // Assert — trailing whitespace trim still works after the
    // continuation-cell skip change.
    assert_eq!(frame.row_text(0), "abc");
    assert_eq!(frame.row_text(0).len(), 3);
}

#[test]
fn ansi_color_codes_are_parsed() {
    // Arrange — red foreground via ANSI escape.
    let data = b"\x1b[31mRed\x1b[0m";

    // Act
    let frame = TerminalFrame::new(80, 24, data);
    let fg_color = frame.fg_color(0, 0);

    // Assert — ANSI index 1 maps to (128, 0, 0).
    assert_eq!(fg_color, Some(CellColor::new(128, 0, 0)));
}

#[test]
fn bold_style_is_detected() {
    // Arrange
    let data = b"\x1b[1mBold\x1b[0m";

    // Act
    let frame = TerminalFrame::new(80, 24, data);
    let style = frame.cell_style(0, 0);

    // Assert
    assert!(style.is_some_and(CellStyle::bold));
}

#[test]
fn cell_text_returns_character() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"Hello");

    // Act / Assert
    assert_eq!(frame.cell_text(0, 0), "H");
    assert_eq!(frame.cell_text(0, 4), "o");
}

#[test]
fn cell_text_returns_space_for_empty_cell() {
    // Arrange
    let frame = TerminalFrame::new(80, 24, b"A");

    // Act — cell beyond written content should be a space.
    let text = frame.cell_text(0, 5);

    // Assert
    assert_eq!(text, " ");
}

#[test]
fn dim_style_is_detected() {
    // Arrange — ANSI SGR 2 = dim/faint.
    let data = b"\x1b[2mDim\x1b[0m";

    // Act
    let frame = TerminalFrame::new(80, 24, data);
    let style = frame.cell_style(0, 0);

    // Assert
    assert!(style.is_some_and(CellStyle::dim));
    assert!(!style.is_some_and(CellStyle::bold));
}

#[test]
fn contents_formatted_roundtrips_colors() {
    // Arrange — red foreground via ANSI escape.
    let data = b"\x1b[31mRed\x1b[0m Plain";
    let frame = TerminalFrame::new(80, 24, data);

    // Act — reconstruct from formatted bytes.
    let formatted = frame.contents_formatted();
    let reconstructed = TerminalFrame::new(80, 24, &formatted);

    // Assert — color and text are preserved.
    assert_eq!(
        reconstructed.fg_color(0, 0),
        Some(CellColor::new(128, 0, 0))
    );
    assert_eq!(reconstructed.row_text(0), frame.row_text(0));
}

#[test]
fn ansi_index_to_rgb_standard_colors() {
    // Arrange / Act / Assert
    assert_eq!(ansi_index_to_rgb(0), CellColor::black());
    assert_eq!(ansi_index_to_rgb(15), CellColor::white());
    assert_eq!(ansi_index_to_rgb(9), CellColor::new(255, 0, 0));
}

#[test]
fn ansi_index_to_rgb_grayscale_ramp() {
    // Arrange / Act
    let darkest = ansi_index_to_rgb(232);
    let lightest = ansi_index_to_rgb(255);

    // Assert
    assert_eq!(darkest, CellColor::new(8, 8, 8));
    assert_eq!(lightest, CellColor::new(238, 238, 238));
}

#[test]
fn cell_style_preserves_all_terminal_flags() {
    // Arrange
    let frame = TerminalFrame::new(3, 1, b"\x1b[1;3;4;7mA\x1b[0;2mB\x1b[0mC");

    // Act
    let styled = frame.cell_style(0, 0).expect("styled cell");
    let dimmed = frame.cell_style(0, 1).expect("dimmed cell");
    let plain = frame.cell_style(0, 2).expect("plain cell");

    // Assert
    assert!(styled.bold());
    assert!(dimmed.dim());
    assert!(styled.italic());
    assert!(styled.underline());
    assert!(styled.inverse());
    assert!(!plain.bold());
    assert!(!plain.dim());
    assert!(!plain.italic());
    assert!(!plain.underline());
    assert!(!plain.inverse());
}
