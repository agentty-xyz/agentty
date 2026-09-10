use std::path::PathBuf;

use ratatui::layout::Alignment;

use super::ProjectSwitcherOverlay;
use crate::domain::project::{Project, ProjectListItem};
use crate::ui::Component;
use crate::ui::style::palette;

/// Builds one project row fixture for switcher render tests.
fn project_list_item_fixture(id: i64, name: &str, active_session_count: u32) -> ProjectListItem {
    ProjectListItem {
        active_session_count,
        input_tokens: 0,
        last_session_updated_at: None,
        output_tokens: 0,
        project: Project {
            created_at: 0,
            display_name: Some(name.to_string()),
            git_branch: Some("main".to_string()),
            id,
            is_favorite: false,
            last_opened_at: None,
            path: PathBuf::from(format!("/tmp/{name}")),
            updated_at: 0,
        },
        session_count: 7,
    }
}

/// Flattens a rendered test buffer into a plain string for text checks.
fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    buffer
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

#[test]
fn test_project_switcher_overlay_popup_area_grows_with_project_count() {
    // Arrange
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    let first_project = project_list_item_fixture(1, "agentty", 3);
    let second_project = project_list_item_fixture(2, "service", 1);
    let project_items = vec![&first_project, &second_project];
    let overlay = ProjectSwitcherOverlay::new(&project_items, 1, 0);

    // Act
    let popup_area = overlay.popup_area(area);

    // Assert
    assert_eq!(popup_area.width, 42);
    assert_eq!(popup_area.height, 10);
    assert_eq!(popup_area.x, 19);
    assert_eq!(popup_area.y, 7);
}

#[test]
fn test_project_switcher_overlay_render_shows_projects_and_hints() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let first_project = project_list_item_fixture(1, "agentty", 3);
    let second_project = project_list_item_fixture(2, "service", 1);
    let project_items = vec![&first_project, &second_project];
    let overlay = ProjectSwitcherOverlay::new(&project_items, 1, 0);

    // Act
    terminal
        .draw(|frame| {
            Component::render(&overlay, frame, frame.area());
        })
        .expect("failed to draw");

    // Assert
    let text = buffer_text(terminal.backend().buffer());
    assert!(text.contains("Switch project"));
    assert!(text.contains("* agentty"));
    assert!(text.contains("service"));
    assert!(text.contains("▶ 3"));
    assert!(text.contains("▶ 1"));
    assert!(text.contains("j/k: move | Enter: switch | q: close"));
}

#[test]
fn test_project_switcher_overlay_render_aligns_project_labels() {
    // Arrange
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");
    let first_project = project_list_item_fixture(1, "agentty", 3);
    let second_project = project_list_item_fixture(2, "service", 1);
    let project_items = vec![&first_project, &second_project];
    let overlay = ProjectSwitcherOverlay::new(&project_items, 1, 0);

    // Act
    terminal
        .draw(|frame| {
            Component::render(&overlay, frame, frame.area());
        })
        .expect("failed to draw");

    // Assert
    let buffer = terminal.backend().buffer();
    let active_label_column =
        text_start_column(buffer, "agentty").expect("active project label should render");
    let inactive_label_column =
        text_start_column(buffer, "service").expect("inactive project label should render");
    assert_eq!(active_label_column, inactive_label_column);
}

/// Finds the first column where `needle` starts in the test buffer.
fn text_start_column(buffer: &ratatui::buffer::Buffer, needle: &str) -> Option<u16> {
    let needle_symbols = needle
        .chars()
        .map(|character| character.to_string())
        .collect::<Vec<_>>();
    let width = usize::from(buffer.area.width.max(1));

    for row in buffer.content().chunks(width) {
        for (column_index, window) in row.windows(needle_symbols.len()).enumerate() {
            let window_matches = window
                .iter()
                .zip(&needle_symbols)
                .all(|(cell, symbol)| cell.symbol() == symbol);

            if window_matches {
                return u16::try_from(column_index).ok();
            }
        }
    }

    None
}

#[test]
fn test_project_switcher_overlay_lines_highlight_selected_row() {
    // Arrange
    let first_project = project_list_item_fixture(1, "agentty", 3);
    let second_project = project_list_item_fixture(2, "service", 1);
    let project_items = vec![&first_project, &second_project];
    let overlay = ProjectSwitcherOverlay::new(&project_items, 1, 1);

    // Act
    let lines = overlay.lines(20);
    let selected_line = &lines[3];
    let unselected_line = &lines[2];

    // Assert
    assert!(
        selected_line
            .spans
            .iter()
            .all(|span| span.style.bg == Some(palette::accent()))
    );
    assert!(
        unselected_line
            .spans
            .iter()
            .all(|span| span.style.bg != Some(palette::accent()))
    );
}

#[test]
fn test_project_switcher_overlay_lines_show_only_running_session_counts() {
    // Arrange
    let busy_project = project_list_item_fixture(1, "agentty", 2);
    let idle_project = project_list_item_fixture(2, "service", 0);
    let project_items = vec![&busy_project, &idle_project];
    let overlay = ProjectSwitcherOverlay::new(&project_items, 1, 0);

    // Act
    let lines = overlay.lines(20);
    let busy_row_text: String = lines[2]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    let idle_row_text: String = lines[3]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();

    // Assert
    assert!(busy_row_text.contains("▶ 2"));
    assert_eq!(idle_row_text.trim(), "service");
}

#[test]
fn test_project_switcher_overlay_lines_mark_active_project() {
    // Arrange
    let first_project = project_list_item_fixture(1, "agentty", 3);
    let second_project = project_list_item_fixture(2, "service", 1);
    let project_items = vec![&first_project, &second_project];
    let overlay = ProjectSwitcherOverlay::new(&project_items, 2, 0);

    // Act
    let lines = overlay.lines(20);
    let active_row_text: String = lines[3]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
    let inactive_row_text: String = lines[2]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();

    // Assert
    assert!(active_row_text.contains("* service"));
    assert!(!inactive_row_text.contains('*'));
}

#[test]
fn test_project_switcher_overlay_lines_center_header_and_help_text() {
    // Arrange
    let first_project = project_list_item_fixture(1, "agentty", 0);
    let project_items = vec![&first_project];
    let overlay = ProjectSwitcherOverlay::new(&project_items, 1, 0);

    // Act
    let lines = overlay.lines(20);
    let header_line = lines.first().expect("overlay should include a header line");
    let help_line = lines.last().expect("overlay should include a help line");

    // Assert
    assert_eq!(header_line.alignment, Some(Alignment::Center));
    assert_eq!(help_line.alignment, Some(Alignment::Center));
}
