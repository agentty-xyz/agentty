use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::HelpOverlay;
use crate::presentation::app_mode::HelpContext;
use crate::presentation::help_action::HelpAction;
use crate::ui::Component;

/// Renders the help overlay into an in-memory terminal and returns its rows.
fn render_rows(context: &HelpContext, width: u16, height: u16) -> Vec<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("failed to create terminal");

    terminal
        .draw(|frame| HelpOverlay::new(context).render(frame, frame.area()))
        .expect("failed to render help overlay");

    buffer_rows(terminal.backend().buffer())
}

/// Collects one string per buffer row.
fn buffer_rows(buffer: &Buffer) -> Vec<String> {
    (0..buffer.area.height)
        .map(|row| {
            (0..buffer.area.width)
                .map(|column| buffer[(column, row)].symbol())
                .collect()
        })
        .collect()
}

/// Builds a list-mode help context with `count` copies of one shortcut.
fn repeated_quit_context(count: usize) -> HelpContext {
    HelpContext::List {
        keybindings: (0..count)
            .map(|_| HelpAction::new("quit", "q", "Quit"))
            .collect(),
    }
}

#[test]
fn test_popup_area_fits_rows_and_chrome() {
    // Arrange
    let area = Rect::new(0, 0, 100, 50);

    // Act
    let popup = HelpOverlay::popup_area(area, "Keybindings", 30, 5);

    // Assert — 30 row columns + 6 chrome columns, 5 rows + 4 chrome rows
    assert_eq!(popup, Rect::new(32, 20, 36, 9));
}

#[test]
fn test_popup_area_grows_to_fit_title_and_hint() {
    // Arrange
    let area = Rect::new(0, 0, 100, 50);

    // Act
    let title_popup = HelpOverlay::popup_area(area, "A very long keybindings title", 4, 1);
    let hint_popup = HelpOverlay::popup_area(area, "Keys", 4, 1);

    // Assert — title needs 29 + 4 columns; " Esc: close " needs 12 + 2 columns,
    // wider than both the 4 + 4 title columns and the 4 + 6 row columns
    assert_eq!(title_popup.width, 33);
    assert_eq!(hint_popup.width, 14);
}

#[test]
fn test_popup_area_grows_to_fit_scroll_hint_when_overflowing() {
    // Arrange — 47 rows + 4 chrome rows exceed the 50-row area
    let area = Rect::new(0, 0, 100, 50);

    // Act
    let popup = HelpOverlay::popup_area(area, "Keys", 4, 47);

    // Assert — " j/k: scroll | Esc: close " needs 26 + 2 columns
    assert_eq!(popup.width, 28);
    assert_eq!(popup.height, 50);
}

#[test]
fn test_popup_area_clamps_to_area_when_small() {
    // Arrange
    let area = Rect::new(0, 0, 20, 8);

    // Act
    let popup = HelpOverlay::popup_area(area, "Keybindings", 40, 20);

    // Assert
    assert_eq!(popup, area);
}

#[test]
fn test_help_overlay_new_stores_fields() {
    // Arrange
    let context = repeated_quit_context(1);

    // Act
    let overlay = HelpOverlay::new(&context).scroll_offset(5);

    // Assert
    assert_eq!(overlay.scroll_offset, 5);
}

#[test]
fn test_help_overlay_has_no_empty_rows_and_shows_close_hint() {
    // Arrange
    let context = HelpContext::List {
        keybindings: vec![
            HelpAction::new("quit", "q", "Quit"),
            HelpAction::new("help", "?", "Help"),
        ],
    };

    // Act
    let rows = render_rows(&context, 80, 24);

    // Assert — top border, padding, 2 rows, padding, bottom border
    let popup_rows: Vec<&String> = rows.iter().filter(|row| !row.trim().is_empty()).collect();
    assert_eq!(popup_rows.len(), 6);
    assert!(popup_rows[0].contains("Keybindings"));
    assert!(popup_rows[2].contains("q: Quit"));
    assert!(popup_rows[3].contains("?: Help"));
    assert!(popup_rows[5].contains("Esc: close"));
    assert!(!popup_rows[5].contains("j/k: scroll"));
}

#[test]
fn test_help_overlay_shows_scroll_hint_only_when_entries_overflow() {
    for (action_count, expects_hint) in [(20, false), (21, true)] {
        // Arrange
        let context = repeated_quit_context(action_count);

        // Act
        let rows = render_rows(&context, 80, 24);

        // Assert
        let text = rows.concat();
        assert_eq!(text.contains("j/k: scroll | Esc: close"), expects_hint);
        assert!(text.contains("Esc: close"));
    }
}
