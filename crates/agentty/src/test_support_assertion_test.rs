use std::path::Path;
use std::process::Command;

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Style};

use super::{
    TestSubscriber, rendered_text_start_cell, rendered_text_start_cells, setup_test_git_repo,
};

#[test]
fn git_repo_fixtures_are_independent_committed_main_checkouts() {
    // Arrange
    let first = tempfile::tempdir().expect("failed to create first fixture dir");
    let second = tempfile::tempdir().expect("failed to create second fixture dir");
    let git = |path: &Path, args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("failed to run git");

        String::from_utf8(output.stdout).expect("git output should be UTF-8")
    };

    // Act
    setup_test_git_repo(first.path());
    setup_test_git_repo(second.path());
    std::fs::write(first.path().join("README.md"), "changed").expect("failed to edit fixture");

    // Assert
    assert_eq!(
        git(second.path(), &["branch", "--show-current"]).trim(),
        "main"
    );
    assert_eq!(
        git(second.path(), &["log", "--format=%s"]).trim(),
        "Initial commit"
    );
    assert_eq!(git(second.path(), &["status", "--porcelain"]), "");
    assert_eq!(
        git(first.path(), &["status", "--porcelain"]).trim(),
        "M README.md"
    );
}

#[test]
fn test_subscriber_records_span_and_event_fields() {
    // Arrange
    let subscriber = TestSubscriber;

    // Act
    let span_registered = tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!(
            "test_subscriber_span",
            recorded_value = tracing::field::Empty
        );
        span.record("recorded_value", "recorded");
        if let Some(span_id) = span.id() {
            span.follows_from(span_id);
        }
        {
            let _guard = span.enter();
            tracing::warn!(path = %Path::new("/workspace").display(), "test warning");
        }

        span.id().is_some()
    });

    // Assert
    assert!(span_registered);
}

#[test]
fn rendered_text_start_cell_returns_first_match() {
    // Arrange
    let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 12, 2));
    buffer.set_string(1, 0, "one", Style::default().fg(Color::Green));
    buffer.set_string(1, 1, "one", Style::default().fg(Color::Yellow));

    // Act
    let cell = rendered_text_start_cell(&buffer, "one").expect("text should render");

    // Assert
    assert_eq!(cell.fg, Color::Green);
}

#[test]
fn rendered_text_start_cells_returns_all_matches() {
    // Arrange
    let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 12, 2));
    buffer.set_string(1, 0, "same", Style::default().fg(Color::Green));
    buffer.set_string(1, 1, "same", Style::default().fg(Color::Yellow));

    // Act
    let cells = rendered_text_start_cells(&buffer, "same");
    let colors = cells.iter().map(|cell| cell.fg).collect::<Vec<_>>();

    // Assert
    assert_eq!(colors, vec![Color::Green, Color::Yellow]);
}

#[test]
fn rendered_text_start_cell_returns_none_for_missing_text() {
    // Arrange
    let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 12, 1));
    buffer.set_string(1, 0, "present", Style::default());

    // Act
    let cell = rendered_text_start_cell(&buffer, "missing");

    // Assert
    assert!(cell.is_none());
}
