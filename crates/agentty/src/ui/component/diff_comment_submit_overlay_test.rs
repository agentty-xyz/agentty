use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Cell;
use ratatui::layout::Rect;
use ratatui::text::Line;

use super::DiffCommentSubmitOverlay;
use crate::ui::Component;

/// Renders `overlay` into a `width` by `height` test terminal and returns the
/// buffer rows as text.
fn render_rows(overlay: &DiffCommentSubmitOverlay<'_>, width: u16, height: u16) -> Vec<String> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|frame| {
            let area = frame.area();
            Component::render(overlay, frame, area);
        })
        .expect("draw");

    terminal
        .backend()
        .buffer()
        .content()
        .chunks(usize::from(width))
        .map(|row| row.iter().map(Cell::symbol).collect())
        .collect()
}

/// Joins a rendered line's span text.
fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

#[test]
fn test_render_shows_title_comment_preview_and_choices() {
    // Arrange
    let preview = "File comments:\n- src/lib.rs: rename this module";
    let overlay = DiffCommentSubmitOverlay::new(preview, true);

    // Act
    let rows = render_rows(&overlay, 100, 30);

    // Assert
    let text = rows.join("\n");
    assert!(text.contains("Submit diff comments?"));
    assert!(text.contains("File comments:"));
    assert!(text.contains("- src/lib.rs: rename this module"));
    assert!(text.contains(" Yes "));
    assert!(text.contains(" No "));
}

#[test]
fn test_fit_preview_lines_reports_hidden_rows_when_preview_overflows() {
    // Arrange
    let lines = ["one", "two", "three", "four", "five"]
        .map(Line::from)
        .to_vec();

    // Act
    let fitted_lines = DiffCommentSubmitOverlay::fit_preview_lines(lines, 3);

    // Assert
    let texts: Vec<String> = fitted_lines.iter().map(line_text).collect();
    assert_eq!(texts, vec!["one", "two", "... 3 more lines"]);
}

#[test]
fn test_fit_preview_lines_keeps_fitting_preview_unchanged() {
    // Arrange
    let lines = ["one", "two"].map(Line::from).to_vec();

    // Act
    let fitted_lines = DiffCommentSubmitOverlay::fit_preview_lines(lines, 2);

    // Assert
    let texts: Vec<String> = fitted_lines.iter().map(line_text).collect();
    assert_eq!(texts, vec!["one", "two"]);
}

#[test]
fn test_popup_grows_with_preview_up_to_screen_cap() {
    // Arrange
    let area = Rect::new(0, 0, 100, 30);
    let short_overlay = DiffCommentSubmitOverlay::new("one", true);
    let medium_preview = ["row"; 12].join("\n");
    let medium_overlay = DiffCommentSubmitOverlay::new(&medium_preview, true);
    let long_preview = ["row"; 60].join("\n");
    let long_overlay = DiffCommentSubmitOverlay::new(&long_preview, true);

    // Act
    let (short_area, _) = short_overlay.popup_area_and_preview(area);
    let (medium_area, medium_lines) = medium_overlay.popup_area_and_preview(area);
    let (long_area, _) = long_overlay.popup_area_and_preview(area);

    // Assert
    assert_eq!(short_area.height, 8);
    assert_eq!(medium_lines.len(), 12);
    assert_eq!(medium_area.height, 18);
    assert_eq!(long_area.height, 27);
    assert_eq!(long_area.width, 70);
}

#[test]
fn test_render_keeps_choices_visible_for_overflowing_preview() {
    // Arrange
    let preview = ["row"; 60].join("\n");
    let overlay = DiffCommentSubmitOverlay::new(&preview, false);

    // Act
    let rows = render_rows(&overlay, 80, 20);

    // Assert
    let text = rows.join("\n");
    assert!(text.contains("more lines"));
    assert!(text.contains(" Yes "));
    assert!(text.contains(" No "));
}

#[test]
fn test_option_line_highlights_selected_choice() {
    // Arrange
    let yes_overlay = DiffCommentSubmitOverlay::new("", true);
    let no_overlay = DiffCommentSubmitOverlay::new("", false);

    // Act
    let yes_line = yes_overlay.option_line();
    let no_line = no_overlay.option_line();

    // Assert
    assert_eq!(yes_line.spans[0].style, no_line.spans[2].style);
    assert_eq!(yes_line.spans[2].style, no_line.spans[0].style);
    assert_ne!(yes_line.spans[0].style, yes_line.spans[2].style);
}
