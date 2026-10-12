use std::path::PathBuf;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;

use super::ProjectCatalogOverlay;
use crate::domain::input::InputState;
use crate::presentation::project_catalog::ProjectCatalogDialog;
use crate::ui::Component;

#[test]
fn project_dialogs_render_scope_choices_selection_errors_and_controls() {
    // Arrange
    let cases = [
        (
            ProjectCatalogDialog::SuggestHome {
                root: PathBuf::from("/home/test"),
                selected: 1,
            },
            "Skip",
        ),
        (
            ProjectCatalogDialog::SuggestHome {
                root: PathBuf::from("/home/test"),
                selected: 0,
            },
            "Discover",
        ),
        (
            ProjectCatalogDialog::PathInput {
                discover: false,
                error: None,
                input: InputState::with_text("~/repo".to_string()),
            },
            "Add project",
        ),
        (
            ProjectCatalogDialog::PathInput {
                discover: true,
                error: None,
                input: InputState::default(),
            },
            "folder-access",
        ),
        (
            ProjectCatalogDialog::PathInput {
                discover: true,
                error: Some("Choose a directory".to_string()),
                input: InputState::default(),
            },
            "Choose a directory",
        ),
        (
            ProjectCatalogDialog::Scanning {
                root: PathBuf::from("/chosen/code"),
            },
            "Esc: cancel scan",
        ),
        (
            ProjectCatalogDialog::Results {
                error: None,
                paths: vec![
                    (PathBuf::from("/first"), false),
                    (PathBuf::from("/second"), true),
                ],
                selected: 1,
            },
            "[x] /second",
        ),
        (
            ProjectCatalogDialog::Results {
                error: Some("Cannot save".to_string()),
                paths: Vec::new(),
                selected: 0,
            },
            "Cannot save",
        ),
        (
            ProjectCatalogDialog::Message {
                message: "No repositories found".to_string(),
            },
            "No repositories found",
        ),
    ];
    for (dialog, expected) in cases {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
        // Act
        terminal
            .draw(|frame| ProjectCatalogOverlay { dialog: &dialog }.render(frame, frame.area()))
            .expect("draw");
        // Assert
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        assert!(text.contains(expected), "missing {expected}: {text}");
    }
}

#[test]
fn startup_suggestion_visually_selects_skip() {
    // Arrange
    let dialog = ProjectCatalogDialog::SuggestHome {
        root: PathBuf::from("/home/test"),
        selected: 1,
    };
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    // Act
    terminal
        .draw(|frame| ProjectCatalogOverlay { dialog: &dialog }.render(frame, frame.area()))
        .expect("draw");
    // Assert
    let cells = terminal.backend().buffer().content();
    let skip = cells
        .windows(4)
        .find(|span| {
            span.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
                == "Skip"
        })
        .expect("Skip choice");
    assert!(
        skip.iter()
            .all(|cell| cell.modifier.contains(Modifier::BOLD))
    );
}
