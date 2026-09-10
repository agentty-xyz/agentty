use ratatui::layout::Rect;

use super::StackAppendParentOverlay;
use crate::domain::session::Status;
use crate::ui::Component;
use crate::ui::style::palette;

#[test]
fn test_stack_append_parent_overlay_renders_candidates_and_hint() {
    // Arrange
    let first_session =
        crate::test_support::titled_session_fixture("first-session", Status::Review);
    let mut second_session =
        crate::test_support::titled_session_fixture("second-session", Status::AgentReview);
    second_session.title = Some("Second parent".to_string());
    let parent_sessions = vec![&first_session, &second_session];
    let overlay = StackAppendParentOverlay::new(&parent_sessions, 1);
    let backend = ratatui::backend::TestBackend::new(80, 24);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    terminal
        .draw(|frame| Component::render(&overlay, frame, frame.area()))
        .expect("failed to draw");

    // Assert
    let text = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect::<String>();
    assert!(text.contains("Append to stack"));
    assert!(text.contains("Choose parent session"));
    assert!(text.contains("Second parent"));
    assert!(text.contains("Enter: append"));
    let selected_line = &overlay.lines(30, 10)[3];
    assert!(
        selected_line
            .spans
            .iter()
            .any(|span| span.style.bg == Some(palette::accent()))
    );
}

#[test]
fn test_stack_append_parent_overlay_height_grows_with_candidates() {
    // Arrange
    let first_session =
        crate::test_support::titled_session_fixture("first-session", Status::Review);
    let second_session =
        crate::test_support::titled_session_fixture("second-session", Status::Review);
    let parent_sessions = vec![&first_session, &second_session];
    let overlay = StackAppendParentOverlay::new(&parent_sessions, 0);

    // Act
    let popup_area = overlay.popup_area(Rect::new(0, 0, 80, 24));

    // Assert
    assert_eq!(popup_area.width, 52);
    assert_eq!(popup_area.height, 10);
}

#[test]
fn test_stack_append_parent_overlay_windows_over_height_candidates_around_selection() {
    // Arrange
    let sessions = (0..10)
        .map(|index| {
            let mut session = crate::test_support::titled_session_fixture(
                &format!("parent-{index}"),
                Status::Review,
            );
            session.title = Some(format!("Parent {index}"));

            session
        })
        .collect::<Vec<_>>();
    let parent_sessions = sessions.iter().collect::<Vec<_>>();
    let overlay = StackAppendParentOverlay::new(&parent_sessions, 8);
    let backend = ratatui::backend::TestBackend::new(80, 12);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create terminal");

    // Act
    terminal
        .draw(|frame| Component::render(&overlay, frame, frame.area()))
        .expect("failed to draw");
    let text = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect::<String>();

    // Assert
    assert!(text.contains("Parent 8"));
    assert!(!text.contains("Parent 0"));
    assert!(text.contains("Enter: append"));
}
